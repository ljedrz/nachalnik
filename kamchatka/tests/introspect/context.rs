//! `context` reading itself: what `look` lists, what `budget` says it costs, and what `request`
//! reports is going out.

use crate::{agent, answered, answers_from, branch, one_turn};
use kamchatka::{introspect, tools::Careful, tools::Limits, tools::Subject};
use nachalnik::{
    Config, ContextItem, ContextState, Kernel, ToolCallId, Verdict, test::ScriptedProvider,
    test::call,
};
use serde_json::json;
use std::sync::Arc;

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
        branch(&spec.schema, "look")["properties"]["whole"]["type"],
        "boolean",
        "`whole` is read, and advertised in three places: {}",
        spec.schema
    );

    kernel.push(ContextItem::file("noise.log", long.clone()));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let results = answers_from(&kernel, &["context"]);
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
async fn an_action_that_is_no_part_of_this_tool_gets_the_list_of_the_ones_that_are() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
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
        "context",
        json!({
            "action": "elide",
            "ids": [1],
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

/// A turn whose thinking the endpoint will not take back is ranked by what it sends.
///
/// note: the model-facing half of the figure the pane was missing. Under any OpenAI-compatible
/// endpoint an assistant turn's reasoning is held and never sent, so a turn that thought at
/// length holds tens of thousands of tokens and puts a few hundred into the request - and a list
/// headed "the most expensive item(s) actually going into it", ranked by what each item *holds*,
/// put it at the top. That is an offer of 25,903 tokens for an elision that frees a thousand,
/// under a note whose whole point is that it does not offer what giving something up would not
/// buy. It ranks on the column that decides now, and says what the row is holding beside it.
/// Every state `budget` says the model sets is one this tool has an action for.
///
/// note: `budget` named `archived` among "three states you set" for as long as `archive` was an
/// action, and went on naming it after `archive` was merged into `exclude` and the word left the
/// vocabulary. A tool that tells a model about a state and gives it no way to reach one is the
/// exact failure the moves were renamed to close - two models in a row spent a call each asking
/// for an `action` called `restore` before it was one. The check is against `CHANGES` rather than
/// against a list written here, so the sentence cannot outlive the word again.
#[tokio::test]
async fn budget_promises_no_state_the_vocabulary_cannot_reach() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "budget" }),
    )]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("what is this costing?"));
    kernel.turn().await.expect("the turn ran");

    let said = answered(&kernel);
    // the clause between the two, however it is punctuated: the states this sentence hands the
    // model as its own to set
    let claimed = (said.split_once("held back:").expect("the held-back line").1)
        .split_once("you set")
        .expect("and the clause saying which of them the model sets")
        .0;

    for state in ["archived", "pinned", "superseded"] {
        assert!(
            !claimed.contains(state),
            "`budget` offers `{state}` as a state the model sets, and no action leaves one: {said}"
        );
    }
    for state in ["excluded", "elided"] {
        assert!(claimed.contains(state), "{said}");
    }
    // archived is still worth naming, as somewhere items arrive rather than somewhere to send them
    assert!(said.contains("archived"), "{said}");
}

#[tokio::test]
async fn the_expensive_list_ranks_by_what_a_row_sends_not_by_what_it_holds() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "budget" }),
    )]));
    kernel.set_projector(Arc::new(nachalnik::LinearProjector {
        send_reasoning: false,
        ..Default::default()
    }));

    kernel.push(ContextItem::file("big.rs", "0".repeat(4_000)));
    // holds far more than `big.rs` and sends a fraction of it
    kernel.push(
        ContextItem::assistant("done - the deck reads better now", Vec::new())
            .with_reasoning(Some("weighing the two openings. ".repeat(600).into())),
    );
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn ran");

    let said = answered(&kernel);
    let listed: Vec<&str> = said
        .lines()
        .skip_while(|line| !line.contains("most expensive"))
        .collect();
    let at = |needle: &str| {
        listed
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("`{needle}` is not in the list: {}", listed.join("\n")))
    };
    assert!(
        at("big.rs") < at("the deck reads better"),
        "the turn holds more and sends less, so it ranks below: {}",
        listed.join("\n")
    );

    // and what it is holding is said on its row rather than left out of the account entirely
    let row = listed[at("the deck reads better")];
    assert!(
        row.contains("holding") && row.contains("the request does not carry"),
        "{row}"
    );
    assert!(
        said.contains("thinking this endpoint will not take back"),
        "the figure above the list does not say what the fourth way of being held back is: {said}"
    );

    // and it divides the four in place rather than counting them off. It used to end `Only the
    // first three are yours to change`, meaning the first three causes - and a live session read
    // it as items 1, 2 and 3, which is a fair reading when every other number on the screen is an
    // item id and the table below opens with a column of them. It spent the next four calls
    // hunting for what items 1-3 were hiding, reached outside the sandbox, and dumped the whole
    // log looking for them
    assert!(
        !said.contains("the first three"),
        "an ordinal here reads as item ids, because the table under it is a column of them: {said}"
    );
    // note: it was `three states you set` until `archive` stopped being an action, which made the
    // count wrong as well as the ordinal - the model sets two of the four now, and `archived` is
    // somewhere items arrive rather than somewhere to send them. Naming them is the part that
    // matters and the part this holds; the number was never the point
    assert!(
        said.contains("excluded or elided to a marker, which you set"),
        "so the ones that are the caller's are named as states instead: {said}"
    );
    assert!(
        said.contains("which is not yours to change"),
        "and the fourth is marked where it is said, not by counting: {said}"
    );
}

/// And `look` reports both figures, because one of them is always the wrong answer to something.
#[tokio::test]
async fn look_says_what_each_item_sends_and_what_it_is_holding_out() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "context", json!({ "action": "look" })),
        call("c2", "context", json!({ "action": "look", "ids": [2] })),
    ]));
    kernel.set_projector(Arc::new(nachalnik::LinearProjector {
        send_reasoning: false,
        ..Default::default()
    }));

    kernel.push(ContextItem::user("rewrite the page"));
    kernel.push(
        ContextItem::assistant("done - the deck reads better now", Vec::new())
            .with_reasoning(Some("weighing the two openings. ".repeat(600).into())),
    );

    kernel.turn().await.expect("the turn ran");

    let said = answers_from(&kernel, &["context"]);
    assert_eq!(said.len(), 2, "{said:?}");

    // the listing: two columns, and the turn's row carries a figure in each
    let (listing, read_back) = (&said[0], &said[1]);
    assert!(
        listing.contains("sending") && listing.contains("held"),
        "{listing}"
    );
    let row = listing
        .lines()
        .find(|line| line.contains("the deck reads better"))
        .expect("the turn is listed");
    let figures: Vec<usize> = row
        .split_whitespace()
        .filter_map(|word| word.replace(',', "").parse().ok())
        .collect();
    // the identifier, what it sends, and what it holds out - the last of them the largest by far
    assert!(
        figures.len() >= 3 && figures[2] > figures[1] * 10,
        "the row does not report both figures: {row}"
    );

    // and it says what that column means, which a live run is the reason for: this model read the
    // two figures correctly, named the turn holding 1,398 tokens of its own thinking, and answered
    // that yes, it could free them by eliding it - which would free the 68 the turn was sending
    // and none of the rest. The same sentence in front of the same question, one run later, came
    // back "doing so would free only its 68 currently sending tokens"
    assert!(
        listing.contains("frees what it is `sending` and none of what it is holding"),
        "nothing says what `held` means for a decision: {listing}"
    );

    // the turn carrying the call being answered has no result yet, so the projector drops it: `0`
    // under `sending`, and the projector's own words for why. Also from the live run, where a
    // model was shown that `0` about its own latest turn with nothing to account for it
    assert!(
        listing.contains("· not going: an assistant turn with no content and no answered calls"),
        "the row that is not going does not say why: {listing}"
    );

    // and reading the item back says the same thing in words, where the thinking itself is
    assert!(
        read_back.contains("held back from the next request"),
        "{read_back}"
    );
    assert!(
        read_back.contains("weighing the two openings"),
        "the thinking is still read back in full: {read_back}"
    );
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
        &Subject::Capability(kamchatka::tools::domains::context("look")),
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
