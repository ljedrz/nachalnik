//! Tests for the tools an agent inspects and manages its own context with.
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

use kamchatka::{
    introspect,
    tools::{Careful, Limits, Subject},
};
use nachalnik::{
    Capability, Config, ContextItem, ContextKind, ContextState, Kernel, ModelResponse, Role,
    ToolCallId, Verdict,
    test::{ScriptedProvider, call},
};
use serde_json::json;

/// A kernel with the tools installed, the provider that will answer it, and the handle the tools
/// reach it through - which the caller has to hold on to, or they stop working.
///
/// note: the policy this program ships, with the four capabilities answered, rather than
/// `AllowAll`. It is one line longer and it is the configuration the program is actually in - and
/// `setup permissions` reports the policy's own table, so a suite that handed it a policy the
/// kernel was not consulting would be checking a sentence about the wrong thing.
fn agent(
    script: impl IntoIterator<Item = ModelResponse>,
) -> (Kernel, Arc<ScriptedProvider>, Arc<Kernel>) {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(ScriptedProvider::new(script));
    kernel.set_provider(provider.clone());
    let policy = Arc::new(Careful::new());
    for capability in ["context", "log", "setup", "amend"] {
        policy.set(
            &Subject::Capability(Capability::Custom(capability.into())),
            Verdict::Allow,
        );
    }
    kernel.set_policy(policy.clone());
    let anchor = introspect::install(&kernel, policy, Limits::default());

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
    answers_from(kernel, &["context", "amend"])
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
        "context",
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

/// And the reason it gives is the reason for *that* change. One sentence used to serve every
/// action, so writing a note - which grows the request because that is what a note is for - was
/// told its ten extra tokens were a marker left by an elision it had not performed.
#[tokio::test]
async fn a_note_that_makes_the_request_bigger_is_not_blamed_on_an_elision() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "note",
            "label": "code word",
            "content": "the code word is PELICAN",
            "reason": "worth keeping",
        }),
    )]));

    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("the next request is now"), "{said}");
    assert!(
        !said.contains("marker"),
        "no marker was left anywhere: {said}"
    );
    assert!(
        !said.contains("not less"),
        "and a note costing what it says is not a surprise to account for: {said}"
    );
}

/// Content coming back into the request gets its own account of why the figure went up, rather
/// than the elision one or none at all.
#[tokio::test]
async fn restoring_something_says_the_growth_is_the_content_itself() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "amend",
            json!({ "action": "exclude", "ids": [1], "reason": "not needed for now" }),
        )]),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "amend",
            json!({ "action": "restore", "ids": [1], "reason": "needed after all" }),
        )]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user(
        "a message long enough that leaving it out and putting it back moves the figure by more \
         than nothing at all, which is the whole of what this is checking",
    ));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("more than before"), "{said}");
    assert!(said.contains("costs what it says"), "and why: {said}");
    assert!(!said.contains("marker"), "nothing was elided: {said}");
}

/// A move needs to know which items, and `label` is not how it is said - but it is a way somebody
/// could reasonably think it was, because `label` is in the same schema. So the refusal is the
/// spelling of what they meant rather than a restatement of the arguments.
#[tokio::test]
async fn a_move_given_a_label_instead_of_ids_is_told_how_to_say_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "elide",
            "label": "secrets.txt",
            "reason": "of no further interest",
        }),
    )]));

    kernel.push(ContextItem::file("secrets.txt", "nothing much"));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains(r#"select: "label:secrets.txt""#),
        "the exact thing to say next: {said}"
    );
    assert!(
        kernel.items().iter().all(|item| !item.state.is_elided()),
        "and nothing was moved on a guess"
    );
}

#[tokio::test]
async fn a_long_item_comes_back_as_a_sample_unless_the_whole_of_it_is_asked_for() {
    // the trap this closes: reading an item copies it into the context, so asking to see a big
    // tool result in order to decide whether to keep it costs about what keeping it costs. A live
    // session did that twice and finished an honest clean-up heavier than the waste it removed
    let long = format!("HEAD-MARKER\n{}\nTAIL-MARKER", "noise line\n".repeat(2_000));
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "context", json!({ "action": "look", "ids": [1] })),
        call(
            "c2",
            "context",
            json!({ "action": "look", "ids": [1], "whole": true }),
        ),
    ]));

    // an argument the tool reads and its own output tells the model to use is one the schema has
    // to declare: a model following the schema cannot pass it otherwise, and an endpoint
    // validating against the schema refuses the call outright
    let spec = kernel.tool("context").expect("it is installed").spec();
    assert_eq!(
        spec.schema["properties"]["whole"]["type"], "boolean",
        "`whole` is read, and advertised in three places: {}",
        spec.schema
    );

    kernel.push(ContextItem::file("noise.log", long.clone()));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let results: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| item.label == "context")
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
        "context",
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
        "context",
        json!({ "action": "request" }),
    )]));

    kernel.push(ContextItem::system("be brief"));
    let file = kernel.push(ContextItem::file("secrets.env", "TOKEN=hunter2"));
    kernel.push(ContextItem::user("what now?"));
    kernel.set_state([file], ContextState::Excluded, Some("not yours".into()));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("left out by its own state"), "{said}");
    assert!(said.contains("not yours"), "{said}");
    assert!(said.contains("system"), "{said}");
    // and the way back is named beside it, because this is the half of the list a state change
    // reaches
    assert!(said.contains("`restore`"), "{said}");
    // the request is summarized, never quoted: printing it would double every token being asked
    // about, and the excluded item's contents would come back in the answer
    assert!(!said.contains("hunter2"), "{said}");
}

/// The two ways an item goes missing are answered differently, so they are reported apart.
///
/// note: an orphaned tool result is the case. Its state says `active` and it is costing nothing,
/// because the projector repairs it out of a request whose assistant turn no longer asks for it -
/// so `restore` on it does exactly nothing, and one undifferentiated list of what was left out is
/// a list in which the cheap guess is the useless move. What there is to fix is the cause.
#[tokio::test]
async fn request_says_which_rule_left_each_item_out() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "request" }),
    )]));

    kernel.push(ContextItem::user("go on"));
    let turn = kernel.push(ContextItem::assistant(
        "looking",
        vec![call("gone", "shell", json!({}))],
    ));
    kernel.push(ContextItem::tool_result(
        ToolCallId("gone".into()),
        "shell",
        "the output nothing asked for any more",
        false,
    ));
    let excluded = kernel.push(ContextItem::file("notes.md", "something"));
    kernel.set_state([excluded], ContextState::Excluded, Some("too big".into()));
    // the turn that asked goes, and its answer is orphaned - active, and going nowhere
    kernel.set_state(
        [turn],
        ContextState::Excluded,
        Some("said nothing useful".into()),
    );

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("left out by its own state"), "{said}");
    assert!(said.contains("too big"), "{said}");
    assert!(
        said.contains("left out by the projector"),
        "the other half is named, and named after the thing that decided: {said}"
    );
    assert!(
        said.contains("LinearProjector"),
        "by the name it can be looked up under: {said}"
    );
    assert!(
        said.contains("Restoring these changes nothing"),
        "and says why `restore` is the wrong move on that half: {said}"
    );
    assert!(said.contains("orphaned tool result"), "{said}");
}

#[tokio::test]
async fn draft_answers_on_a_fork_and_leaves_the_context_alone() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "context", json!({ "action": "draft" }))]),
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
            "context",
            json!({ "action": "fork", "question": "why did you stop?" }),
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
            "context",
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
        "context",
        json!({ "action": "look" }),
    )]));

    kernel.push(ContextItem::user("look at yourself"));
    // whoever installed them has gone; the tools are still registered and still answer, and what
    // they answer is why they cannot do anything
    drop(anchor);

    kernel.turn().await.expect("the turn failed");

    assert!(answered(&kernel).contains("this session is over"));
}

/// The word for the move is the action, and it is the word the result is read back in.
///
/// note: these five used to be one `prune` action with a `state` argument, which put the word for
/// one of them over all five - `pin` and `restore` included, so "prune to pin it" was the
/// documented way to protect something, and an item you pruned read back as `archived`. Two live
/// models in a row spent a call each asking for `restore` as an action and being told it was a
/// state; they were right and the levels were wrong. The old spelling still works, because
/// accepting a word somebody reached for costs nothing and refusing it costs a turn.
#[tokio::test]
async fn each_move_is_an_action_named_for_what_it_leaves_behind() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "amend",
            json!({ "action": "elide", "ids": [1], "reason": "it is enormous" }),
        ),
        // the way it was spelled before, which is still a way to spell it
        call(
            "c2",
            "amend",
            json!({ "action": "prune", "ids": [2], "state": "exclude", "reason": "and this one" }),
        ),
        // and the way back, which is an action like the rest of them
        call(
            "c3",
            "amend",
            json!({ "action": "restore", "ids": [1], "reason": "I want it after all" }),
        ),
    ]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::file("bigger.rs", "1".repeat(400)));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    assert_eq!(said.len(), 3, "{said:?}");
    assert!(said[0].contains("elided"), "{}", said[0]);
    assert!(said[1].contains("excluded"), "{}", said[1]);
    assert_eq!(kernel.items()[0].state, ContextState::Active, "{}", said[2]);
    assert_eq!(kernel.items()[1].state, ContextState::Excluded);

    // and nothing tells anybody about a `state` argument any more, because there is not one
    let offered = kernel
        .tool_specs()
        .into_iter()
        .find(|spec| spec.id == "amend")
        .expect("it is offered");
    assert!(
        offered.schema["properties"].get("state").is_none(),
        "the level that caused this is still in the schema: {}",
        offered.schema
    );
    for action in ["elide", "exclude", "archive", "pin", "restore"] {
        assert!(
            offered.schema["properties"]["action"]["enum"]
                .as_array()
                .expect("an enum")
                .iter()
                .any(|listed| listed == action),
            "`{action}` is not offered as an action"
        );
    }
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
    assert!(said.contains("elide") && said.contains("revise"), "{said}");
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
        "context",
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

/// Pinning a note costs the person nothing. The undo stack behind the `u` key is theirs, and a
/// note written and *then* pinned was a push and a state change - two checkpoints for one thing
/// the model did, which is the arithmetic `revise` keeps its own account out of the note to avoid.
#[tokio::test]
async fn pinning_a_note_is_not_a_second_thing_to_undo() {
    let written = |pin: bool| async move {
        let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
            "c1",
            "amend",
            json!({
                "action": "note", "label": "the plan", "content": "read the tests first",
                "pin": pin, "reason": "so it outlives this turn",
            }),
        )]));
        kernel.push(ContextItem::user("what is your plan?"));
        kernel.turn().await.expect("the turn ran");

        (
            kernel.with_context(|c| c.undo_len()),
            kernel
                .items()
                .into_iter()
                .find(|item| item.label == "the plan")
                .expect("it was written down")
                .state,
        )
    };

    let (loose, loose_state) = written(false).await;
    let (pinned, pinned_state) = written(true).await;

    assert_eq!(loose_state, ContextState::Active);
    assert_eq!(pinned_state, ContextState::Pinned);
    assert_eq!(
        pinned, loose,
        "the pin cost the person an undo of their own"
    );
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
    // and the reason is on it, which is what a second state change used to be spent putting there
    assert_eq!(
        written.included_because.as_deref(),
        Some("so it outlives this turn")
    );
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
        ModelResponse::tool_calls(vec![call("c1", "context", json!({ "action": "draft" }))]),
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
        ModelResponse::tool_calls(vec![call("c1", "context", json!({ "action": "draft" }))]),
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

#[tokio::test]
async fn hiding_everything_while_holding_no_notes_says_what_that_costs() {
    // the failure this closes: a run gathered nineteen thousand tokens across seventeen tool
    // results, said nothing in its own turns, elided all seventeen in one call, and answered from
    // an empty context - inventing all ten answers. `prune` reported the tokens it had given back
    // and nothing about the evidence it had just taken away
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "amend",
            json!({"action": "prune", "select": "all:tool_results", "state": "elide",
                   "reason": "done with these"}),
        ),
        // a note, and then the same wipe again: with something of its own kept, the warning has
        // nothing to warn about
        call(
            "c2",
            "amend",
            json!({"action": "note", "label": "q1", "content": "Cargo.lock is 3593 lines",
                   "pin": true, "reason": "keeping the finding"}),
        ),
        call(
            "c3",
            "amend",
            json!({"action": "prune", "ids": [2], "state": "elide", "reason": "done with it"}),
        ),
    ]));

    kernel.push(ContextItem::tool_result(
        ToolCallId::from("c0"),
        "shell",
        "3593 Cargo.lock",
        false,
    ));
    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| item.label == "amend")
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(said.len(), 3, "{said:?}");

    assert!(
        said[0].contains("no notes"),
        "a wipe with nothing written down should say so: {}",
        said[0]
    );
    assert!(
        said[0].contains("`note`"),
        "and should name the thing that would have helped: {}",
        said[0]
    );
    // once it has kept something of its own, the same move is no longer the same move
    assert!(
        !said[2].contains("no notes"),
        "a note is in context, so there is nothing to warn about: {}",
        said[2]
    );
    assert!(
        said[2].contains("now elided"),
        "and the prune still happened: {}",
        said[2]
    );
}

// --------------------------------------------------------------------------------------- log

/// A bare call prices the whole log and hands back nothing else.
///
/// note: the estimate is the load-bearing half. "412 records" does not tell a model whether it
/// can afford them, and a summary that named a count and left the cost to be found out by asking
/// would be the thing `budget` exists to stop. So this checks the quote against what the records
/// really cost once they are in the context - the kernel's own count of the item they landed as -
/// rather than only that a figure is there.
#[tokio::test]
async fn a_bare_log_is_a_summary_and_a_price_rather_than_the_records() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({})),
        call("c2", "log", json!({ "since": 0 })),
    ]));

    // enough of them that the answer is mostly records rather than mostly header, which is what
    // makes the two figures comparable at all
    for n in 0..40 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    let item = kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel
        .replace(item, "the parser is in src/parse.rs")
        .unwrap();
    kernel.push(ContextItem::user("what have you been doing?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let summary = &said[0];

    // the kinds, counted, and none of the records
    assert!(summary.contains("context.added"), "{summary}");
    assert!(summary.contains("context.replaced"), "{summary}");
    assert!(
        !summary.contains("src/parser.rs"),
        "a bare call should cost almost nothing, and a replaced item's text is not nothing: \
         {summary}"
    );
    assert!(
        !summary.lines().any(|line| line.starts_with("    1  ")),
        "a bare call hands back no records at all: {summary}"
    );

    // what the summary said the whole log would cost
    let quoted: usize = summary
        .split('~')
        .nth(1)
        .and_then(|rest| rest.split(" tokens").next())
        .map(|n| n.replace(',', "").parse().unwrap())
        .expect("the summary quotes a token figure");

    // and what it really cost: the kernel's count of the item the second answer landed as. It is
    // the later of the two calls, so its log is a few records longer than the one that was priced
    // and it carries a header the quote does not - both push the real figure up, which is the
    // safe direction for an estimate to be wrong in
    let charged = kernel
        .items()
        .iter()
        .filter(|item| matches!(&item.kind, ContextKind::ToolResult { tool, .. } if tool == "log"))
        .nth(1)
        .map(|item| item.tokens)
        .expect("the second answer is in the context");
    assert!(
        quoted <= charged && charged - quoted < charged / 8,
        "the summary quoted {quoted} and taking them all cost {charged}"
    );
}

/// Every answer opens with what exists, not with what matched.
///
/// note: this is the rule that makes the tool safe rather than a convenience. A filtered answer
/// that reported only its own count is indistinguishable from a session in which almost nothing
/// happened, and the difference matters most in exactly the case somebody filters for - looking
/// for an overwrite and finding none.
#[tokio::test]
async fn a_filtered_log_opens_with_the_whole_total_and_not_the_filtered_one() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "kinds": ["context.replaced"] })),
        call("c2", "log", json!({ "kinds": ["model.payload"] })),
    ]));

    kernel.push(ContextItem::user("carry on"));
    let item = kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel
        .replace(item, "the parser is in src/parse.rs")
        .unwrap();
    let total = kernel.history().len();

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    for answer in &said {
        let opens: usize = answer
            .split(' ')
            .next()
            .map(|n| n.replace(',', "").parse().unwrap())
            .expect("every answer opens with a count");
        assert!(
            opens >= total,
            "the answer opened with {opens} and there were {total} records: {answer}"
        );
    }

    assert!(said[0].contains("1 match"), "{}", said[0]);
    // a real zero, arriving beside a total that is not one, which is what stops it reading as an
    // empty log. `model.payload` is a kind nothing emits unless `record_payloads` is on
    assert!(said[1].contains("0 match"), "{}", said[1]);
    assert!(
        said[1].contains("Nothing matched"),
        "a zero says so in words as well as in a figure: {}",
        said[1]
    );
    assert!(
        said[1].contains("context.added"),
        "and lists the kinds there are, because a filter that matched nothing is usually one \
         spelled for another session: {}",
        said[1]
    );
}

/// What an item used to say survives in the log, and `ids` is how it is found again.
///
/// note: measured rather than assumed, and the measurement moved what this test is for. Taking
/// the content out of `ContextReplaced` - the "fix" the stale sentence in `session.rs` used to
/// invite - is already caught by five tests across two crates, `undo::a_replacement_is_the_one_/// thing_that_would_otherwise_be_lost` among them, so this is not the guard on that and saying it
/// was would have been a false sense of a well-watched seam. What nothing else catches is the
/// pair of things this tool adds: finding the record by the *item* number rather than by kind,
/// and `whole` - drop either and only this fails.
#[tokio::test]
async fn a_revised_item_can_be_read_back_out_of_the_log_by_its_number() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "amend",
            json!({
                "action": "revise",
                "ids": [1],
                "content": "the parser is in src/parse.rs",
                "reason": "I wrote down the wrong path",
            }),
        ),
        call("c2", "log", json!({ "ids": [1] })),
        call("c3", "log", json!({ "ids": [1], "whole": true })),
    ]));

    kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs\nand the lexer is in src/lex.rs",
    ));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let (glimpsed, whole) = (&said[0], &said[1]);

    assert!(glimpsed.contains("context.replaced"), "{glimpsed}");
    assert!(glimpsed.contains("src/parser.rs"), "{glimpsed}");
    // the first line only, and the line admits to being one
    assert!(
        !glimpsed.contains("src/lex.rs"),
        "a log line is a line; the rest is asked for: {glimpsed}"
    );
    assert!(glimpsed.contains("`whole`"), "{glimpsed}");

    // and asked for, it is all there - which is the assertion that would fail if anybody ever
    // "fixed" the one event that carries content
    assert!(whole.contains("src/parser.rs"), "{whole}");
    assert!(whole.contains("src/lex.rs"), "{whole}");

    // nothing about reading the log put anything back into the context
    assert_eq!(
        kernel
            .item(nachalnik::ContextId(1))
            .unwrap()
            .content
            .to_text(),
        "the parser is in src/parse.rs"
    );
}

/// A shortened answer says how much of it is missing.
#[tokio::test]
async fn take_says_how_many_records_are_beyond_what_it_showed() {
    let (kernel, _provider, _anchor) =
        agent(one_turn(vec![call("c1", "log", json!({ "take": 2 }))]));

    for n in 0..6 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(said[0].contains("Showing the 2 most recent"), "{}", said[0]);
    assert!(
        said[0].contains("21 older are not here"),
        "a shortened log has to say how much of it is not here, and in a word that is true of \
         them - nothing narrowed what counts here, so they are older rather than unmatched: {}",
        said[0]
    );
    // the most recent, and still in the order they happened
    let numbered: Vec<&str> = said[0]
        .lines()
        .filter(|line| line.starts_with("  ") && line.trim().starts_with(char::is_numeric))
        .collect();
    assert_eq!(numbered.len(), 2, "{}", said[0]);
    let seq = |line: &str| -> u64 { line.trim().split(' ').next().unwrap().parse().unwrap() };
    assert!(
        seq(numbered[0]) < seq(numbered[1]),
        "records come back in the order they happened: {numbered:?}"
    );
}

/// A filter nobody can read is a mistake to report, not a log with nothing in it.
///
/// note: the one wrong answer this tool can give is *nothing happened*, and an empty result for a
/// malformed argument is exactly that answer. The schema is descriptive and the kernel validates
/// nothing against it, so the tool has to.
#[tokio::test]
async fn a_filter_that_is_not_a_number_is_an_error_rather_than_an_empty_log() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "since": "yesterday" })),
        call("c2", "log", json!({ "take": "lots" })),
        // a number written as a word is a mistake; a number written as a string is not, and
        // taking it costs nothing
        call("c3", "log", json!({ "since": "1" })),
    ]));

    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    for answer in &said[..2] {
        assert!(answer.contains("whole number"), "{answer}");
        assert!(
            answer.contains("empty log"),
            "the refusal says why it is not an empty answer: {answer}"
        );
    }
    assert!(said[2].contains("records"), "{}", said[2]);
    assert!(said[2].contains("match since:1"), "{}", said[2]);
}

/// The log is one order, and it is the order the changes were applied in.
///
/// note: the runtime writes the record and the broadcast under one lock so the two agree; what
/// this checks is that reading it back through a tool does not resort it. Run with calls in
/// parallel, because that is the configuration where a second order could appear.
#[tokio::test]
async fn the_records_come_back_in_one_order_however_the_calls_were_run() {
    let kernel = Kernel::new(Config {
        parallel_tool_calls: true,
        ..Config::default()
    });
    let provider = Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "context", json!({ "action": "look" })),
        call("c2", "context", json!({ "action": "budget" })),
        call("c3", "log", json!({ "since": 0 })),
    ])));
    kernel.set_provider(provider);
    let policy = Arc::new(Careful::new());
    for capability in ["context", "log", "setup", "amend"] {
        policy.set(
            &Subject::Capability(Capability::Custom(capability.into())),
            Verdict::Allow,
        );
    }
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());

    kernel.push(ContextItem::user("all three at once"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let seqs: Vec<u64> = said[0]
        .lines()
        .filter(|line| line.starts_with("  ") && line.trim().starts_with(char::is_numeric))
        .map(|line| line.trim().split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(seqs.len() > 3, "{}", said[0]);
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "sequence numbers are the order, and they came back out of it: {seqs:?}"
    );
}

/// The log is read-only, and there is no argument that says otherwise.
#[tokio::test]
async fn log_declares_its_own_capability_and_no_way_to_write() {
    let (kernel, _provider, _anchor) = agent(Vec::new());

    let spec = kernel.tool("log").expect("it is installed").spec();
    assert_eq!(
        spec.capabilities,
        vec![nachalnik::Capability::Custom("log".into())],
        "it has to be separately grantable, and separately revocable"
    );
    assert!(
        spec.schema["properties"]["action"].is_null(),
        "there are no actions here: everything it takes is a filter"
    );
}

// ------------------------------------------------------------------------------------ search

/// The count and the price come before any line, and the archive is searchable at last.
///
/// note: what `look` cannot do. An archived item is kept in full and never sent, and reading one
/// back copies it into the context - so a session that had put eleven megabytes away could not
/// look inside any of it without undoing the saving it had just made. This is the read that does
/// not cost what carrying it costs, and the header is what keeps it that way.
#[tokio::test]
async fn search_prices_the_matches_before_it_shows_one_and_reaches_the_archive() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "search", "text": "landlock" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "search", "text": "landlock", "take": 1 }),
        ),
    ]));

    let put_away = kernel.push(ContextItem::file(
        "notes.md",
        "the sandbox refuses a connect under Landlock\nand has no UDP right at all\nunrelated line",
    ));
    kernel.push(ContextItem::user("what did we say about the sandbox?"));
    kernel.set_state(
        [put_away],
        ContextState::Archived,
        Some("done with it".into()),
    );

    let before = kernel.budget().used();
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    let (counted, shown) = (&said[0], &said[1]);

    // the count, the price and where - and not one of the lines
    assert!(counted.starts_with("1 line(s) say `landlock`"), "{counted}");
    assert!(counted.contains("tokens if you take them all"), "{counted}");
    assert!(
        !counted.contains("refuses a connect"),
        "a bare search hands back the count, not the lines: {counted}"
    );
    // case ignored, and an archived item is searched rather than skipped
    assert!(counted.contains("archived"), "{counted}");

    // asked for, the line arrives
    assert!(
        shown.contains("refuses a connect under Landlock"),
        "{shown}"
    );

    // and the item is exactly where it was: searching is not a way to pay for something
    let item = kernel.item(put_away).expect("still there");
    assert_eq!(item.state, ContextState::Archived);
    assert!(
        kernel.budget().used() > before,
        "the answers themselves are items and cost what they say"
    );
    assert!(
        !kernel.project().included.contains(&put_away),
        "what was archived is still out of the request: searching it restored nothing"
    );
}

/// A search that finds nothing says what it looked at, rather than only that it found nothing.
#[tokio::test]
async fn a_search_with_no_matches_says_what_it_searched() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "search", "text": "seccomp" }),
        ),
        call("c2", "context", json!({ "action": "search" })),
    ]));

    kernel.push(ContextItem::file("notes.md", "Landlock, and nothing else"));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["context"]);
    assert!(
        said[0].contains("no line of your context says `seccomp`"),
        "{}",
        said[0]
    );
    assert!(
        said[0].contains("Case was ignored"),
        "a nil result says how it looked, so it is not read as a fact about the context: {}",
        said[0]
    );
    assert!(
        said[0].contains("archived and excluded items were searched"),
        "{}",
        said[0]
    );
    // and a search with nothing to search for is a mistake to correct
    assert!(said[1].contains("needs the `text`"), "{}", said[1]);
}

/// Narrowed to some items, it looks only in those and says so.
#[tokio::test]
async fn search_can_be_held_to_the_items_it_was_given() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "search", "text": "landlock", "ids": [2] }),
    )]));

    kernel.push(ContextItem::file("a.md", "Landlock is here"));
    kernel.push(ContextItem::file("b.md", "and Landlock is here too"));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.starts_with("1 line(s)"), "{said}");
    assert!(said.contains("looking only in 2"), "{said}");
}

// ------------------------------------------------------------------------------------- setup

/// A tool taken away mid-session is not on the list, which is the point of there being a list.
///
/// note: the shape this exists for. Nothing anywhere let an agent enumerate its own tools, so a
/// model whose `shell` was removed between two turns had no way to find that out and every reason
/// to keep asking for it - or, worse, to explain confidently why it had not used it. With `log`
/// beside it the pair answers both halves: this says what there is, `tools.changed` says when it
/// went.
#[tokio::test]
async fn setup_tools_reflects_a_tool_taken_away_mid_session() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![call("c1", "setup", json!({ "action": "tools" }))]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![call("c2", "setup", json!({ "action": "tools" }))]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what have you got?"));
    kernel.add_tool(Arc::new(nachalnik::test::ConstTool::new(
        "secret", "hunter2",
    )));
    kernel.turn().await.expect("the turn failed");

    // the registry is live and `Tool::spec` is read afresh for every request, which is what makes
    // this a second question rather than a second session
    kernel.remove_tool("secret");
    kernel.push(ContextItem::user("and now?"));
    kernel.turn().await.expect("the second turn failed");

    let said = answers_from(&kernel, &["setup"]);
    assert!(said[0].contains("secret"), "{}", said[0]);
    assert!(
        !said[1].contains("secret"),
        "a tool that has gone is not on the list: {}",
        said[1]
    );
    // and what each one declares, which is the thing a model cannot see and the thing that
    // decides whether a call is worth making at all
    assert!(said[0].contains("nothing declared"), "{}", said[0]);
    assert!(said[0].contains("tools.changed"), "{}", said[0]);
}

/// It says which model it is, and whether the conversation is one it started.
///
/// note: the resumed line is the one that could not be worked out from inside. A context restored
/// from a snapshot carries first-person turns this model never produced, and nothing in a turn
/// records which hand wrote it - so a model asked about its own earlier reasoning in a resumed
/// session owns all of it, because it has no way not to.
#[tokio::test]
async fn setup_model_says_whether_this_conversation_was_inherited() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "model" }),
    )]));
    kernel.push(ContextItem::user("who are you?"));
    kernel.turn().await.expect("the turn failed");

    let fresh = answered(&kernel);
    assert!(
        fresh.contains("started here"),
        "a session nobody resumed says so: {fresh}"
    );

    // the same question on a session resumed from this one's snapshot
    let second = Kernel::resume(Config::default(), kernel.snapshot());
    second.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c2",
        "setup",
        json!({ "action": "model" }),
    )]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(Capability::Custom("setup".into())),
        Verdict::Allow,
    );
    second.set_policy(policy.clone());
    let _anchor = introspect::install(&second, policy, Limits::default());
    second.push(ContextItem::user("who are you?"));
    second.turn().await.expect("the turn failed");

    // note: the *last* one, and the reason is the whole point of the action. The resumed context
    // carries the first session's answer as an item, so the earliest thing `setup model` says in
    // here is the old session's "started here" - written by a model that was right when it wrote
    // it and is being read by one for whom it is false. This is what a model has no way to notice
    // from the inside, which is what the new line is for.
    let carried = answers_from(&second, &["setup"]);
    assert!(
        carried[0].contains("started here"),
        "the inherited answer came along, unchanged and now wrong: {}",
        carried[0]
    );
    let now = carried.last().expect("it answered");
    assert!(now.contains("resumed from a snapshot"), "{now}");
    assert!(
        now.contains("may not be you"),
        "and says the turns in it are not necessarily its own: {now}"
    );
}

/// The verdicts come from the policy's own table, and say which policy that is.
#[tokio::test]
async fn setup_permissions_says_what_will_be_refused_before_it_is_asked() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )]));
    kernel.push(ContextItem::user("what may you do?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    // the policy actually consulted, named, beside the table being reported
    assert!(said.contains("Careful"), "{said}");
    // the four this session answered for, and the word for a thing nobody has decided
    assert!(said.contains("context") && said.contains("allow"), "{said}");
    assert!(
        said.contains("`ask` is nobody having decided yet, not a refusal"),
        "the difference a model has to be able to act on: {said}"
    );
    // a path rule is in the same answer, because it binds the same calls
    assert!(said.contains(".env"), "{said}");
    // and asking is not deciding: reporting the policy must not have changed it
    assert!(kernel.pending_permissions().is_empty(), "{said}");
}

/// What will happen to the context without anybody asking for it, named so it can be looked up.
#[tokio::test]
async fn setup_policy_names_the_seams_that_rewrite_a_context_on_their_own() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "policy" }),
    )]));
    kernel.set_compactor(Some(Arc::new(kamchatka::tools::Trim {
        threshold: 0.8,
        target: 0.6,
    })));
    kernel.push(ContextItem::user("what will be done to me?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("the projector is"), "{said}");
    assert!(said.contains("Trim"), "the compactor by name: {said}");
    assert!(
        said.contains("cannot take anything pinned"),
        "and the promise the kernel keeps against it: {said}"
    );
    assert!(
        said.contains("one at a time, in the order you asked"),
        "{said}"
    );
}

// ------------------------------------------------------- questions the design left open

/// A fork's own events are not in this session's log, and the log says so by counting.
///
/// note: one of the questions the design that brought `log` here left to be resolved while
/// implementing, and the answer falls out of what a fork already is: a whole second kernel with a
/// session of its own that goes when it does. So the parent's log holds what the parent did - it
/// asked for a tool, the tool ran, the tool answered - and the fork's own request is not in it,
/// even though the fork made one against the same provider. What the copy *said* is in the tool
/// result, like any other tool's output, and that is the whole of what crosses.
#[tokio::test]
async fn a_forks_own_events_are_not_in_this_sessions_log() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "context", json!({ "action": "draft" }))]),
        ModelResponse::text("the copy's answer"),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "log",
            json!({ "kinds": ["model.requested"] }),
        )]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what would you say?"));
    kernel.turn().await.expect("the turn failed");

    // four requests reached the provider: the one that asked for `draft`, the fork's own, the one
    // that asked for `log`, and the one that finished the turn
    assert_eq!(provider.requests().len(), 4);

    // and the log this session can read holds two at the moment `log` runs - its own first and
    // third. The fork's is in neither figure, because it belonged to a session that no longer
    // exists, and the fourth had not happened yet
    let said = answers_from(&kernel, &["log"]).remove(0);
    assert!(
        said.contains("2 match kinds:[\"model.requested\"]"),
        "a fork's request is not this session's: {said}"
    );
    // what the copy said did cross, as the tool's output, the way any tool's output does
    assert!(
        all_answers(&kernel)
            .iter()
            .any(|answer| answer.contains("the copy's answer")),
        "the answer is the one thing a fork hands back"
    );
}

/// `by: "amend"` on an item's metadata means the tool, and nothing else in this program writes it.
///
/// note: the other question the design left open - whether a person editing an item would also
/// record `"amend"`, which would have a model reading its own metadata and finding its own tool
/// named as the hand that did it. It would not, and there is no such edit: `Kernel::replace` is
/// reached from `amend`'s `revise` and from `amend`'s `undo`, and from nowhere else in this crate.
/// The `unwrap_or("something")` that suggested otherwise is guarding against metadata written by
/// somebody else's code, which is what a free-form field the kernel never reads is for.
#[tokio::test]
async fn the_only_hand_that_records_itself_as_amend_is_amend() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "revise",
            "ids": [1],
            "content": "the parser is in src/parse.rs",
            "reason": "I wrote it down wrong",
        }),
    )]));

    let item = kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel.push(ContextItem::user("carry on"));

    // a person rewriting an item goes through the same kernel call and records nothing of its own
    kernel
        .replace(item, "edited by whoever is at the terminal")
        .expect("the item is there");
    assert!(
        kernel.item(item).expect("still there").meta.is_null(),
        "nothing outside `amend` attributes an edit to `amend`"
    );

    kernel.turn().await.expect("the turn failed");

    let after = kernel.item(item).expect("still there");
    assert_eq!(after.meta["revised"]["by"], "amend");
    // and both rewrites are on the record with what each replaced, so the two hands are told
    // apart by the log even though the item carries only the second
    let replaced: Vec<String> = kernel
        .history()
        .iter()
        .filter_map(|record| match &record.event {
            nachalnik::Event::ContextReplaced { was, .. } => Some(was.to_text().into_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(replaced.len(), 2, "{replaced:?}");
    assert!(replaced[0].contains("src/parser.rs"), "{replaced:?}");
    assert!(
        replaced[1].contains("whoever is at the terminal"),
        "{replaced:?}"
    );
}

/// `take` shortens an answer; it does not narrow what the answer is of, and the header says so.
///
/// note: found by reading what the tool actually prints rather than by a test, which is why it is
/// worth one now. A call carrying only `take` reported "15 records ... total. 15 match , ~205
/// tokens" - a match count that was really the total, a filter description that was empty because
/// there was no filter, and a dangling comma where it should have been. A header whose whole job
/// is to be believed cannot be the part that reads like a bug.
#[tokio::test]
async fn take_on_its_own_does_not_claim_to_have_matched_anything() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "take": 2 })),
        call(
            "c2",
            "log",
            json!({ "take": 2, "kinds": ["context.added"] }),
        ),
    ]));

    for n in 0..5 {
        kernel.push(ContextItem::memory("scratch", format!("note {n}")));
    }
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let (bare_take, narrowed) = (&said[0], &said[1]);

    assert!(bare_take.contains("tokens in all."), "{bare_take}");
    assert!(
        !bare_take.contains("match"),
        "nothing narrowed what counts, so nothing matched anything: {bare_take}"
    );
    assert!(
        bare_take.contains("older are not here"),
        "what is missing is older, not unmatched: {bare_take}"
    );
    // and where something *did* narrow it, the match count and the filter are both there
    assert!(narrowed.contains("match kinds:"), "{narrowed}");
    assert!(
        narrowed.contains("more match and are not here"),
        "{narrowed}"
    );
}

/// A seam that is generic is named as the type it is, not as the type inside it.
///
/// note: `short` took the last `::` segment of the whole string, so the counter this program
/// ships - `Calibrating<BytesPerToken>` - came out as `BytesPerToken>`: the wrong type, the outer
/// one silently dropped, and a stray bracket as the only sign anything had gone wrong. It was on
/// the trace pane before it was here. A seam names itself so that somebody can look it up, and a
/// name that is not the type's cannot be looked up.
#[tokio::test]
async fn a_generic_seam_is_named_as_itself() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "policy" }),
    )]));
    kernel.push(ContextItem::user("what is plugged in?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("Calibrating<BytesPerToken>"), "{said}");
    assert!(
        !said.contains("BytesPerToken>,"),
        "the outer type is not dropped: {said}"
    );
    // and the module paths are off, because this is read by something paying for every token
    assert!(!said.contains("nachalnik::"), "{said}");
}

/// The undecided path rules are counted rather than listed, the way the permissions tab does it.
#[tokio::test]
async fn setup_permissions_counts_the_rules_nobody_has_thought_about() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "setup",
        json!({ "action": "permissions" }),
    )]));
    kernel.push(ContextItem::user("what may you touch?"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains("path rule(s) are undecided"),
        "a count, not eleven rows of the same verdict: {said}"
    );
    // still named, because an answer standing silently for eleven rules would be its own kind of
    // dishonest - it is the row per rule that is not worth the tokens, not the fact of them
    assert!(said.contains(".env*"), "{said}");
}

/// An argument this tool does not take is a mistake to report, not one to ignore.
///
/// note: found live. A session called `log {action: "look"}` - the sibling tools all take an
/// `action`, so it is the obvious mistake - got the summary back, and read it as the answer to a
/// question it had not asked. It then cited it. An ignored argument is the same failure as a
/// filter nobody can parse, one step earlier: the reply is a real answer, so nothing in it says
/// that what was asked for did not happen.
#[tokio::test]
async fn an_argument_log_does_not_take_is_refused_rather_than_ignored() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "log", json!({ "action": "look" })),
        call(
            "c2",
            "log",
            json!({ "kinds": ["context.added"], "limit": 3 }),
        ),
    ]));
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(said[0].contains("does not take `action`"), "{}", said[0]);
    assert!(
        said[0].contains("no actions here"),
        "the obvious mistake gets the sentence that unmakes it: {}",
        said[0]
    );
    assert!(
        said[0].contains("nothing was read"),
        "and says it did nothing, so it cannot be read as an answer: {}",
        said[0]
    );
    // a real filter beside an unreadable one is still refused, rather than half-honoured
    assert!(said[1].contains("does not take `limit`"), "{}", said[1]);
    assert!(!said[1].contains("match"), "{}", said[1]);
}

/// An item with no beginning in this log is said to have none, which is what `ids` really asks.
///
/// note: the sharpest thing the live runs turned up. A session resumed under a second model was
/// asked whether it had written an inherited turn. It asked the log about that item and got five
/// `model.requested` rows naming it - every one true, because the item had been in every request
/// since - and read them as proof it had written the item itself. The record that settled the
/// question was the one that was not there, and an absence is the one answer a list of matching
/// records cannot give. So the tool says it.
#[tokio::test]
async fn an_inherited_item_is_reported_as_having_no_beginning_here() {
    let first = Kernel::new(Config::default());
    first.push(ContextItem::user("which index?"));
    first.push(ContextItem::assistant("I chose a B-tree.", vec![]));

    let kernel = Kernel::resume(Config::default(), first.snapshot());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "log", json!({ "ids": [2] })),
        call("c2", "log", json!({ "ids": [2, 3] })),
        call("c3", "log", json!({ "ids": [3] })),
    ]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(Capability::Custom("log".into())),
        Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());
    kernel.push(ContextItem::user("and now?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    let (inherited, mixed, native) = (&said[0], &said[1], &said[2]);

    // the misleading evidence is here, exactly as it was live: `model.requested` names item 2,
    // because item 2 has been in every request since. It is true and it is not provenance
    assert!(inherited.contains("model.requested"), "{inherited}");
    // and the record that settles it is the one that is absent, so the absence is stated
    assert!(
        inherited.contains("[2] has no `context.added` here"),
        "{inherited}"
    );
    assert!(inherited.contains("before this log begins"), "{inherited}");
    // and it names the tool that settles it rather than leaving the model to infer a snapshot
    assert!(inherited.contains("`setup` with `model`"), "{inherited}");

    // the same where some of the named items do have a beginning and some do not
    assert!(mixed.contains("[2] has no"), "{mixed}");
    assert!(mixed.contains("context.added"), "{mixed}");
    // and silence where every named item was created here, because then there is nothing to say
    assert!(
        !native.contains("has no `context.added` here"),
        "an item this log saw created needs no note about snapshots: {native}"
    );
}

/// `since` is exclusive, and the schema says which number means everything.
///
/// note: also found live, and it cost the answer. A session reaching for "all of it" wrote
/// `since: 1`, which is *after* record 1 - and in a resumed session record 1 is `session.resumed`,
/// the one record that would have told it the context was not its own. It read everything except
/// the thing it was looking for.
#[tokio::test]
async fn since_one_is_not_since_the_beginning_and_the_schema_says_so() {
    let first = Kernel::new(Config::default());
    first.push(ContextItem::user("earlier"));
    let kernel = Kernel::resume(Config::default(), first.snapshot());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "log", json!({ "since": 1 })),
        call("c2", "log", json!({ "since": 0 })),
    ]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(Capability::Custom("log".into())),
        Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());
    kernel.push(ContextItem::user("and now?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(
        !said[0].contains("session.resumed"),
        "`since: 1` is after record 1, and record 1 is the one that matters: {}",
        said[0]
    );
    assert!(
        said[1].contains("session.resumed"),
        "`since: 0` is the one that means all of them: {}",
        said[1]
    );

    let spec = kernel.tool("log").expect("installed").spec();
    let since = spec.schema["properties"]["since"]["description"]
        .as_str()
        .expect("it says what it is for");
    assert!(
        since.contains("`0` is all of them"),
        "the off-by-one is not guessable, so it is written down: {since}"
    );
}

/// An empty filter is no filter; a filter full of the wrong thing is a mistake.
///
/// note: found live. A model spelling "every argument the schema lists, none of them constraining
/// anything" wrote `{ids: [], kinds: [], since: 0, take: 20}` and was refused, which cost it a
/// turn and taught it nothing - an empty list constrains nothing, and reading it as "no filter" is
/// the only thing it can mean. A list with entries that are not item numbers is a different thing
/// and still worth reporting.
#[tokio::test]
async fn an_empty_filter_list_is_no_filter_rather_than_a_refusal() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "log",
            json!({ "ids": [], "kinds": [], "since": 0, "take": 2, "whole": false }),
        ),
        call("c2", "log", json!({ "ids": ["two"] })),
    ]));
    kernel.push(ContextItem::user("carry on"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["log"]);
    assert!(said[0].contains("match since:0"), "{}", said[0]);
    assert!(
        !said[0].contains("ids:["),
        "an empty list is not reported as a filter that was applied: {}",
        said[0]
    );
    assert!(said[0].contains("Showing the 2 most recent"), "{}", said[0]);
    // and a list of the wrong thing still says so, naming what it was given
    assert!(said[1].contains("none of [\"two\"] is one"), "{}", said[1]);
}

/// `look` says which of the items it lists this session did not produce.
///
/// note: three live runs bought this. A session resumed under a second model, asked whether it had
/// written an inherited turn, reached for `look` every time - and `look` had nothing to say,
/// because a restored item is an ordinary item with no field that marks it. `setup model` had the
/// fact and was never called. A fact only reachable through a tool nobody reaches for is one the
/// program does not really have, so it is said where the question is actually asked.
#[tokio::test]
async fn look_says_which_items_this_session_did_not_produce() {
    let first = Kernel::new(Config::default());
    first.push(ContextItem::user("which index?"));
    first.push(ContextItem::assistant("I chose a B-tree.", vec![]));

    let kernel = Kernel::resume(Config::default(), first.snapshot());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]))));
    let policy = Arc::new(Careful::new());
    policy.set(
        &Subject::Capability(Capability::Custom("context".into())),
        Verdict::Allow,
    );
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());
    kernel.push(ContextItem::user("and now?"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    // first, before the listing, because it is what the listing is about to be misread as
    assert!(
        said.starts_with("this session was resumed from a snapshot"),
        "{said}"
    );
    assert!(said.contains("1, 2"), "and names which ones: {said}");
    assert!(said.contains("not produced here"), "{said}");
    assert!(
        said.contains("`setup` with `model`"),
        "and sends the reader to the tool that says what this model is: {said}"
    );

    // a session nobody resumed says none of it, because there is nothing to say
    let (fresh, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]));
    fresh.push(ContextItem::user("hello"));
    fresh.turn().await.expect("the turn failed");
    assert!(
        !answered(&fresh).contains("resumed from a snapshot"),
        "{}",
        answered(&fresh)
    );
}

/// An answer names only the tools this session actually has.
///
/// note: bought by a live run. `/tools drop log` took the log away mid-session, and `setup tools`
/// went on ending with "`log` with `kinds: [\"tools.changed\"]` says when it went" - advice naming
/// a tool in the same breath as reporting that the model does not have it. It is the rule a
/// refusal already follows: everything named in an answer is read as something to try, so name
/// only what can be reached.
#[tokio::test]
async fn an_answer_does_not_point_at_a_tool_that_has_been_taken_away() {
    let (kernel, _provider, _anchor) = agent(vec![
        ModelResponse::tool_calls(vec![
            call("c1", "setup", json!({ "action": "tools" })),
            call("c2", "setup", json!({ "action": "permissions" })),
        ]),
        ModelResponse::text("done"),
        ModelResponse::tool_calls(vec![
            call("c3", "setup", json!({ "action": "tools" })),
            call("c4", "setup", json!({ "action": "permissions" })),
        ]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what have you got?"));
    kernel.turn().await.expect("the turn failed");

    let with_it = answers_from(&kernel, &["setup"]);
    assert!(with_it[0].contains("tools.changed"), "{}", with_it[0]);
    assert!(with_it[1].contains("permission.decided"), "{}", with_it[1]);

    // the reader of the record goes; the record does not
    kernel.remove_tool("log");
    kernel.push(ContextItem::user("and now?"));
    kernel.turn().await.expect("the second turn failed");

    let without = answers_from(&kernel, &["setup"]);
    for said in &without[2..] {
        assert!(
            !said.contains("`log`"),
            "an answer that names a tool the session does not have is advice nobody can take: \
             {said}"
        );
    }
    // and the rest of the answer is untouched: what is gone is the sentence, not the report
    assert!(
        without[2].contains("tool(s), which is every one"),
        "{}",
        without[2]
    );
    assert!(
        without[3].contains("`ask` is nobody having decided yet"),
        "{}",
        without[3]
    );
}
