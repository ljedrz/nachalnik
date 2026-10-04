//! `context` reading itself: what `look` lists, what `budget` says it costs, and what `request`
//! reports is going out.

use crate::{agent, answered, answers_from, branch, one_turn, tokens_in};
use kamchatka::{introspect, tools::Careful, tools::Limits, tools::Subject};
use nachalnik::{
    Block, Config, Content, ContextItem, ContextKind, ContextState, Kernel, ToolCallId, Verdict,
    test::ScriptedProvider, test::call,
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

/// `whole` written in quotes is read as the word it is, and one that is neither is refused.
#[tokio::test]
async fn the_whole_of_an_item_can_be_asked_for_in_quotes() {
    let long = format!("HEAD-MARKER\n{}\nTAIL-MARKER", "noise line\n".repeat(2_000));
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "look", "ids": [1], "whole": "true" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "look", "ids": [1], "whole": "yes" }),
        ),
    ]));

    kernel.push(ContextItem::file("noise.log", long));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let results = answers_from(&kernel, &["context"]);
    assert!(
        !results[0].contains("bytes not shown"),
        "{:.400}",
        results[0]
    );
    assert!(results[1].contains("true or false"), "{}", results[1]);
}

/// A row is as wide as the column and no wider, and one that fits is not shortened.
///
/// note: the cut is at one character too many rather than at one too few, so a line of exactly
/// the column's width arrives whole and one character more arrives with the mark that says it
/// was cut. The other way round shortens a line that fitted and marks it, and the mark says the
/// text was left off when it was not - which is the one thing a listing cannot say wrongly,
/// since a model reading it either goes looking for the rest of the line or stops.
#[tokio::test]
async fn a_row_that_fits_the_column_is_shown_whole() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]));

    kernel.push(ContextItem::file("wide.rs", "w".repeat(48)));
    kernel.push(ContextItem::file("wider.rs", "n".repeat(49)));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    let row = |file: &str| {
        said.lines()
            .find(|line| line.contains(file))
            .unwrap_or_else(|| panic!("no row for {file}: {said}"))
            .to_owned()
    };

    let fits = row("wide.rs");
    assert!(
        fits.contains(&"w".repeat(48)) && !fits.contains('…'),
        "a line as wide as the column is the whole of it: {fits}"
    );

    let over = row("wider.rs");
    assert!(
        over.contains('…') && !over.contains(&"n".repeat(49)),
        "and one character more is cut, with the mark that says so: {over}"
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

/// A row says how many calls its turn made, and says nothing at all about a turn that made none.
///
/// note: `[0 call(s)]` on a turn that only talked would be a figure about nothing, and a model
/// reading it as an instrument panel for its own turn is being told it asked for something when
/// it did not - which is the one way a listing of what the model did can be confidently wrong.
#[tokio::test]
async fn a_row_counts_the_calls_a_turn_made_and_only_where_it_made_some() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]));

    kernel.push(ContextItem::user("read both"));
    kernel.push(ContextItem::assistant(
        "reading one of them",
        vec![call(
            "r1",
            "fs",
            json!({ "action": "read", "path": "one.rs" }),
        )],
    ));
    kernel.push(ContextItem::tool_result(
        ToolCallId("r1".into()),
        "fs",
        "the first file",
        false,
    ));
    // and a turn that only said something, which is what most of them are
    kernel.push(ContextItem::assistant("both are parsers", vec![]));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    let row = |item: &ContextItem| {
        said.lines()
            .find(|line| {
                line.split_whitespace()
                    .next()
                    .is_some_and(|id| id == item.id.0.to_string())
            })
            .unwrap_or_else(|| panic!("item {} is not listed: {said}", item.id.0))
    };
    // the turn that asked for one file, by its row rather than by its index: the turn being run
    // pushes items of its own
    let items = kernel.items();
    let asking = items
        .iter()
        .find(|item| item.calls().next().is_some() && item.id.0 < 5)
        .expect("the turn that read a file is in the context");
    assert!(
        row(asking).contains("[1 call(s)]"),
        "the turn that asked for one file says so: {said}"
    );
    assert!(
        !said.contains("[0 call(s)]"),
        "a turn that asked for nothing is not a figure about zero calls: {said}"
    );
}

/// A sample is for an item too big to copy into the context; a middling one is read whole.
///
/// note: the cut is at three thousand bytes, and the answer for anything under it is the whole
/// item. An item of a couple of thousand bytes is exactly what a clean-up run meets first - one
/// file read, one tool result - and cutting it means the model pays for a marker to read a
/// paragraph it could have had whole.
#[tokio::test]
async fn an_item_of_middling_size_is_read_back_whole() {
    let middling = "a line of a file the model read. ".repeat(70);
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look", "ids": [1] }),
    )]));

    kernel.push(ContextItem::file("one.rs", middling.clone()));
    kernel.push(ContextItem::user("read it back"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        !said.contains("bytes not shown"),
        "an item of {} bytes is not a sample: {said:.200}",
        middling.len()
    );
    assert!(
        said.contains(&middling),
        "and it comes back as itself: {said:.200}"
    );
}

/// A sample is cut on a character boundary, or the whole read back fails.
///
/// note: `&text[..head]` and `&text[tail..]` are byte ranges, and both ends of the cut are
/// computed in bytes, so a cut that did not look for a boundary would end inside a character -
/// a panic in the middle of answering a `look`, which is the one call an agent makes when it has
/// already decided it is in trouble.
#[tokio::test]
async fn a_sample_of_multibyte_text_is_cut_on_character_boundaries() {
    // a three-byte character at each cut, so byte 1500 and byte len-1500 each land inside one: a
    // euro at 1499..1502 and again at 3000..3003, of a 4,501-byte item
    let euro = "\u{20ac}";
    let mut item = "h".repeat(1499);
    item.push_str(euro);
    item.push_str(&"x".repeat(1498));
    item.push_str(euro);
    item.push_str(&"t".repeat(1498));
    assert_eq!(
        item.len(),
        4_501,
        "the fixture is the shape the test is about"
    );

    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look", "ids": [1] }),
    )]));

    kernel.push(ContextItem::file("multibyte.rs", item.clone()));
    kernel.push(ContextItem::user("read it back"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains("bytes not shown"),
        "an item this size is a sample: {said:.200}"
    );
    // the head stops at the last boundary at or before 1500, which is 1499 where the first euro
    // starts, and the tail starts at the first boundary at or after len-1500, which is 3003 where
    // the second one ends: both cuts on a boundary, and the figure between them the gap
    assert!(
        said.contains("1,504 bytes not shown"),
        "the gap is the two cuts' distance: {said:.200}"
    );
    assert!(
        said.contains(&"h".repeat(1499)),
        "the head is everything up to the boundary: {said:.200}"
    );
    assert!(
        said.contains(&"t".repeat(1498)),
        "and the tail is everything after it: {said:.200}"
    );
}

/// A turn recorded as ordered blocks is read back with what it asked for, block by block.
///
/// note: this is the view of a call that exists nowhere else. The request the model will be sent
/// has the same parts in the same order, but there a call is a field beside the turn; and a
/// flattened turn has no order to read at all. Without the arm for a call, the row for a call
/// reads `call:` and nothing - the block's own name, and no trace of the tool, the arguments or
/// the identifier it will be answered under.
#[tokio::test]
async fn a_turn_read_back_shows_its_calls_among_its_blocks() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look", "ids": [1] }),
    )]));

    kernel.push(ContextItem::new(
        ContextKind::AssistantMessage {
            tool_calls: Vec::new(),
            reasoning: None,
        },
        "model",
        "assistant",
        Content::blocks([
            Block::text(Content::text("reading both of them")),
            Block::Call(nachalnik::ToolCall::new(
                "r1",
                "fs",
                json!({ "action": "read", "path": "one.rs" }),
            )),
        ]),
    ));
    kernel.push(ContextItem::user("go on"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("block(s), in order"), "{said}");
    assert!(
        said.contains(r#"call: fs({"action":"read","path":"one.rs"})"#),
        "a call block reads as the call it is, and not as its name alone: {said}"
    );
    assert!(
        said.contains("reading both of them"),
        "and the text block beside it is still read: {said}"
    );
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

/// A turn that only called a tool is as big as the arguments it sent, not as the nothing it said.
#[tokio::test]
async fn request_sizes_a_message_by_what_it_sends() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "request" }),
    )]));

    kernel.push(ContextItem::user("write it"));
    kernel.push(
        ContextItem::assistant(
            "",
            vec![call(
                "w1",
                "fs",
                json!({ "action": "write", "content": "x".repeat(5_000) }),
            )],
        )
        // and the thinking beside it, which goes out in the message's own reasoning slot and is
        // as much of what the turn sends as the arguments it wrote are
        .with_reasoning(Some("weighing the two openings. ".repeat(100).into())),
    );
    kernel.push(ContextItem::tool_result(
        ToolCallId("w1".into()),
        "fs",
        "wrote it",
        false,
    ));
    kernel.push(ContextItem::user("and now?"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    let row = said
        .lines()
        .find(|line| line.trim_start().starts_with("2  assistant"))
        .unwrap_or_else(|| panic!("no row for the turn that wrote: {said}"));
    let bytes: usize = row
        .split_whitespace()
        .nth(2)
        .map(|figure| figure.replace(',', ""))
        .and_then(|figure| figure.parse().ok())
        .unwrap_or_else(|| panic!("no size on {row:?}"));
    assert!(bytes > 5_000, "{row}");
    // both halves of the turn, and neither of them counted as nothing: reasoning the endpoint
    // will not take back is `held` in `look`, and this is what it sends
    let thought = "weighing the two openings. ".repeat(100).len();
    assert!(
        bytes > 5_000 + thought,
        "the column carries what it sends, and the reasoning is in the request: {row}"
    );
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

    // each half names what is in it, which is the only thing that tells the two apart: the item
    // a state took out is reported under the state and beside `restore`, and the item the
    // projector took out under the projector and beside the sentence saying restoring it is the
    // wrong move. One list holding the other's words would send a model to `restore` an orphan
    let half = |heading: &str| {
        let said = answered(&kernel);
        let (start, rest) = said
            .split_once(heading)
            .unwrap_or_else(|| panic!("no `{heading}` in: {said}"));
        // the heading, then the lines under it and not the next section's: a paragraph breaks
        // each half off from what follows it
        let under = rest.split_once("\n\n").map_or(rest, |(this, _)| this);
        start.lines().last().unwrap_or_default().to_owned() + under
    };
    let by_state = half("left out by its own state");
    assert!(by_state.contains("too big"), "{by_state}");
    assert!(
        !by_state.contains("orphaned tool result"),
        "an active item the projector took out is in the other half: {by_state}"
    );

    let by_projector = half("left out by the projector");
    assert!(
        by_projector.contains("orphaned tool result"),
        "{by_projector}"
    );
    assert!(
        !by_projector.contains("too big"),
        "an excluded item is in the other half, where `restore` would reach it: {by_projector}"
    );
}

/// Each of the projector's two adjustments is reported under its own heading, and neither
/// heading is printed over an empty list.
///
/// note: they are two different pieces of news and only one of them is a fault. `repairs` is
/// content the model would have had and will not; `reordered` is the layout rule working, and a
/// line reading like something broke - over an event that costs nothing and is nothing to act on
/// - stays in the conversation for the rest of the session. A model reading a request summary
/// counts a heading as a list it can read, so a heading over nothing is an answer about a fault
/// that did not happen.
#[tokio::test]
async fn request_reports_each_projection_adjustment_under_its_own_heading() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "request" }),
    )]));

    kernel.push(ContextItem::user("go on"));
    kernel.push(ContextItem::assistant(
        "looking",
        vec![call("w1", "shell", json!({ "action": "ls" }))],
    ));
    // a user turn between the call and its answer, which is what `context: note` does on every
    // call - the item is written while the call that writes it is still in flight
    kernel.push(ContextItem::user("meanwhile"));
    kernel.push(ContextItem::tool_result(
        ToolCallId("w1".into()),
        "shell",
        "the listing",
        false,
    ));
    // and a result nothing asked for any more, which the projector has to take out
    kernel.push(ContextItem::tool_result(
        ToolCallId("gone".into()),
        "shell",
        "the output of a call nobody makes",
        false,
    ));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    let section = |heading: &str| {
        let rest = said
            .split_once(heading)
            .unwrap_or_else(|| panic!("no `{heading}` in: {said}"))
            .1;
        rest[..rest.find("\n\n").unwrap_or(rest.len())].to_owned()
    };

    // the one that lost content is under the heading for it, with the call that was taken down
    let repairs = section("and what that same projector rewrote");
    assert!(
        repairs.contains("`gone` is not in the projection"),
        "content the model would have had went out, and it says what: {repairs}"
    );

    // and the one that only moved is under the heading saying nothing was lost, and not under
    // that one: the two are different news, and a line reading like a fault about an event that
    // costs nothing stays in the conversation for the rest of the session
    let reordered = section("and what it put in a different order");
    assert!(
        reordered.contains("moved item"),
        "the move is reported: {reordered}"
    );
    assert!(
        reordered.contains("nothing to act on"),
        "and said out loud that there is nothing to do about it: {reordered}"
    );
    assert!(
        !reordered.contains("`gone`"),
        "the dropped result is not in this half either: {reordered}"
    );

    // and where the projector adjusted nothing, neither heading is there to be read
    let (whole, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "request" }),
    )]));
    whole.set_projector(Arc::new(nachalnik::LinearProjector {
        repair_orphans: false,
        ..Default::default()
    }));
    whole.push(ContextItem::user("write it"));
    whole.push(ContextItem::assistant(
        "writing",
        vec![call("w1", "fs", json!({ "action": "write" }))],
    ));
    whole.push(ContextItem::tool_result(
        ToolCallId("w1".into()),
        "fs",
        "wrote it",
        false,
    ));
    whole.turn().await.expect("the turn failed");
    let projection = whole.project();
    assert!(
        projection.repairs.is_empty() && projection.reordered.is_empty(),
        "and this one needed neither: {projection:?}"
    );

    let said = answered(&whole);
    assert!(
        !said.contains("and what that same projector rewrote"),
        "nothing was taken out, so nothing is said about taking out: {said}"
    );
    assert!(
        !said.contains("and what it put in a different order"),
        "nothing moved, so nothing is said about moving: {said}"
    );
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
    // and the list is every operation the tool has, not only the ones that change something. The
    // eight that change are what a call naming nothing falls through to asking for a `reason`
    // about, so a list of those would be an answer to a call about writing rather than to a call
    // about a word nobody has
    assert!(
        said.contains("look") && said.contains("search"),
        "the list is of what this tool does, and four of its operations only read: {said}"
    );
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

/// The fifth column of the expensive list is the running total of the fourth.
///
/// note: what the model is asked to read the list for. `budget` exists so a compaction decision
/// can be made, and the decision is "where do I cut" - which is a question about the sum of the
/// costs above a row, not about the cost of the row. A column that stopped adding said a drop
/// would free nothing.
#[tokio::test]
async fn the_running_column_of_the_expensive_list_is_the_sum_of_the_one_beside_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "budget" }),
    )]));

    // three files, so the list has three rows and their totals are told apart from one another
    kernel.push(ContextItem::file("a.rs", "a".repeat(4_000)));
    kernel.push(ContextItem::file("b.rs", "b".repeat(2_000)));
    kernel.push(ContextItem::file("c.rs", "c".repeat(8_000)));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn ran");

    let figure = |it: &str| it.replace(',', "").parse::<usize>();
    let rows: Vec<(usize, usize)> = answered(&kernel)
        .lines()
        .filter(|line| line.contains("active"))
        .filter_map(|line| {
            // id, state, kind, sending, if all go, and what the row is holding
            let columns: Vec<&str> = line.split_whitespace().collect();
            Some((figure(columns.get(3)?).ok()?, figure(columns.get(4)?).ok()?))
        })
        .collect();
    assert!(
        rows.len() >= 3,
        "three items are going into the request, so each has a row: {rows:?}"
    );

    let mut sum = 0;
    for (sending, running) in &rows {
        sum += sending;
        assert_eq!(
            *running, sum,
            "eliding everything down to this row frees what the rows above it send"
        );
    }
}

/// Every state `budget` says the model sets is one this tool has an action for.
///
/// note: `budget` named `archived` among "three states you set" for as long as `archive` was an
/// action, and went on naming it after `archive` was merged into `exclude` and the word left the
/// vocabulary. A tool that tells a model about a state and gives it no way to reach one is the
/// exact failure the moves were renamed to close - two models in a row spent a call each asking
/// for an `action` called `restore` before it was one.
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

    // a pin holds nothing back, so it is not among them
    assert!(
        !claimed.contains("pinned"),
        "`budget` offers `pinned` as a way of holding tokens back: {said}"
    );
    for state in ["excluded", "elided"] {
        assert!(claimed.contains(state), "{said}");
    }
    // and what the program excludes is said as excluded, which is one word for one thing: a note
    // walked back and the whole of a shortened answer are out the same way anything else is
    assert!(said.contains("the program excludes too"), "{said}");
    assert!(!said.contains("archived"), "{said}");
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
    // count wrong as well as the ordinal - the model sets two of the states, and what the program
    // excludes on its own is said beside them rather than under a word of its own. Naming them is
    // the part that matters and the part this holds; the number was never the point
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

/// A picture is counted as nothing, and every figure this tool gives says so.
///
/// note: the context tab marks such a row and `/budget` says every figure is a floor; this tool
/// said neither, so the model read a context carrying a screenshot as a small one - its account of
/// its own budget drifting from the person's, which `Going` is shared to prevent.
#[tokio::test]
async fn unpriced_content_is_said_to_be_wherever_a_figure_is_given() {
    let (kernel, _provider, _anchor) = agent(Vec::new());
    let shot = kernel.push(ContextItem::user(Content::blob(
        "image/png",
        "iVBORw0KGgo=",
    )));
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![
        call("c1", "context", json!({ "action": "budget" })),
        call("c2", "context", json!({ "action": "look" })),
        call(
            "c3",
            "context",
            json!({ "action": "look", "ids": [shot.0] }),
        ),
        call(
            "c4",
            "context",
            json!({ "action": "look", "select": "user" }),
        ),
    ]))));

    kernel.turn().await.expect("the turn failed");

    let said: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(said.len(), 4, "{said:?}");
    assert!(
        said[0].contains("every figure above is a floor"),
        "{}",
        said[0]
    );
    assert!(said[1].contains("a `+` is a floor"), "{}", said[1]);
    assert!(
        said[2].contains("1 piece(s) nothing here can price"),
        "{}",
        said[2]
    );
    // a listing narrowed by a selector is the same listing, and the picture no cheaper in it
    assert!(said[3].contains("a `+` is a floor"), "{}", said[3]);
    assert!(
        said[3]
            .lines()
            .any(|line| line.contains("user_message") && line.contains("0+")),
        "and the picture's own row carries the mark: {}",
        said[3]
    );
}

/// A narrowed listing reads its figures against the context, not against the whole request.
///
/// note: the header put a part and a whole beside each other that were not one. `sending` and
/// `held` are summed over the matched items; the figure they were read against was
/// `budget().used()`, which is every item *plus* the tool definitions, which have no row in the
/// table under it. So a listing of four tool results read `~1,531 tokens going ... out of ~6,107
/// the whole request carries` and the missing 4,576 - most of it the JSON schemas of the tools
/// the model is holding - was accounted to nothing. On a context that is mostly reasoning the
/// first two figures exceeded the third outright, which is not something a part can do to the
/// whole it is part of.
///
/// note: the request's own total is still here, with the tool definitions named in it, because a
/// model budgeting its own work is also asking how much of the request is not its context. Two
/// lines rather than one, and each on one basis.
#[tokio::test]
async fn a_narrowed_listing_reads_its_figures_against_the_context() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look", "select": "all" }),
    )]));

    // a turn that thought at length and sent a fraction of it, which is the shape that made the
    // two figures exceed the whole
    kernel.set_projector(Arc::new(nachalnik::LinearProjector {
        send_reasoning: false,
        ..Default::default()
    }));
    kernel.push(
        ContextItem::assistant("the answer", Vec::new())
            .with_reasoning(Some("weighing it. ".repeat(600).into())),
    );
    for n in 0..3 {
        kernel.push(ContextItem::file(
            format!("f{n}.rs"),
            "some file the model read. ".repeat(200),
        ));
    }
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    let context_line = said
        .lines()
        .find(|line| line.contains("are those items"))
        .unwrap_or_else(|| panic!("the figures are on their own line: {said}"));
    // the two figures on it are a part and the whole they are part of
    let figures = tokens_in(context_line);
    assert!(figures.len() >= 2, "{context_line}");
    assert!(
        figures[0] <= figures[1],
        "the matched items are a part of the context, so they cannot be more than it: \
         {context_line}"
    );
    assert!(
        !context_line.contains("the whole request carries"),
        "the request is not the whole of what the class is a part of: {context_line}"
    );
    // and the request's own total is named with what is in it
    let request_line = said
        .lines()
        .find(|line| line.contains("the next request is"))
        .unwrap_or_else(|| panic!("the request's own figures are named: {said}"));
    assert!(
        request_line.contains("is the tool definitions"),
        "{request_line}"
    );
    let figures = tokens_in(request_line);
    assert!(
        figures[1] < figures[0],
        "the tool definitions are a part of the request, and named as such: {request_line}"
    );
}

/// The headline counts what the request carries, and an item repaired out of it is not carried.
#[tokio::test]
async fn look_counts_what_the_projection_carries_rather_than_the_states() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]));

    let turn = kernel.push(ContextItem::assistant("", vec![crate::call_of("t1")]));
    kernel.push(ContextItem::tool_result(
        ToolCallId::from("t1"),
        "shell",
        "an answer to a call that is no longer there",
        false,
    ));
    kernel.push(ContextItem::user("go on"));
    kernel.set_state([turn], ContextState::Excluded, Some("taken out".into()));

    kernel.turn().await.expect("the turn failed");

    // the user's line and the turn this call is in; not the result whose call was set aside
    let said = answered(&kernel);
    assert!(
        said.contains("4 items · 2 of them go into the next request"),
        "{said}"
    );
}

/// The notes the model wrote are a class it can name, and the description says which: a model
/// that looked for them as `all:memories` found nothing, since a memory is another source.
#[tokio::test]
async fn the_models_own_notes_are_named_in_the_description_and_found_by_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "context",
            json!({ "action": "note", "label": "finding", "content": "the parser is in src/parse.rs", "reason": "to keep it" }),
        ),
        call(
            "c2",
            "context",
            json!({ "action": "look", "select": "source:agent" }),
        ),
    ]));
    let description = kernel.tool("context").expect("offered").spec().description;
    assert!(description.contains("`source:agent`"), "{description}");

    kernel.push(ContextItem::user("write it down"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("finding"), "{said}");
    assert!(!said.contains("nothing in your context matches"), "{said}");
}

/// A full listing accounts for the numbers it skips, and says nothing where it skips none.
///
/// note: found live: after an `undo` took 25 away, a model reading `24, 26` said it could not tell
/// whether 25 was gone or not shown.
#[tokio::test]
async fn look_accounts_for_the_numbers_it_skips() {
    let gap = "the numbers skip where an item was removed";

    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]));
    kernel.push(ContextItem::user("one"));
    kernel.push(ContextItem::user("two"));
    kernel.push(ContextItem::user("three"));
    assert!(kernel.undo().expect("undo"), "the last push was undoable");
    kernel.push(ContextItem::user("four"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(
        said.contains(gap),
        "a gap in the ids is accounted for: {said}"
    );
    assert!(
        said.contains("no longer in the context, not left off this list"),
        "and named as removed rather than hidden: {said}"
    );

    // and with no gap, the line is not there to misread
    let (whole, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "context",
        json!({ "action": "look" }),
    )]));
    whole.push(ContextItem::user("one"));
    whole.push(ContextItem::user("two"));
    whole.turn().await.expect("the turn failed");
    assert!(
        !answered(&whole).contains(gap),
        "an unbroken listing says nothing of gaps: {}",
        answered(&whole)
    );
}
